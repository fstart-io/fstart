//! EHCI debug console selection on southbridges with two EHCI controllers
//! (coreboot `USBDEBUG_HCD_INDEX` and `CONFIG_EHCI_BAR`).

use fstart_driver_usbdebug::Config;

/// coreboot `CONFIG_EHCI_BAR` for ICH9 and Ibex Peak: one 4 KiB page below
/// the local APIC, clear of the fixed HPET, I/O APIC and flash decodes.
const EHCI_DEBUG_BAR: u32 = 0xfef0_0000;
const EHCI_DEBUG_BAR_SIZE: u64 = 0x1000;

/// An EHCI controller. The debug port is a fixed root port of each
/// controller, so a board selects the one wired to its debug connector
/// (coreboot `USBDEBUG_HCD_INDEX` 1 or 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EhciController {
    Ehci1,
    Ehci2,
}

impl EhciController {
    /// The board's controller, or EHCI2 then EHCI1 when it names none.
    /// `None` when the chosen controller, or every one, is disabled.
    pub fn select(choice: Option<Self>, enabled: impl Fn(Self) -> bool) -> Option<Self> {
        match choice {
            Some(controller) => enabled(controller).then_some(controller),
            None => [Self::Ehci2, Self::Ehci1]
                .into_iter()
                .find(|&controller| enabled(controller)),
        }
    }
}

/// The debug console on bus-0 `devfn` at [`EHCI_DEBUG_BAR`], or `None` when
/// that BAR would overlap the `ecam_buses` of ECAM or one of the chipset's
/// relocatable `decodes` (base, size).
pub fn config(
    devfn: u8,
    ecam_base: usize,
    ecam_buses: u16,
    decodes: &[(u64, u64)],
) -> Option<Config> {
    let bar = u64::from(EHCI_DEBUG_BAR);
    let ecam = (ecam_base as u64, u64::from(ecam_buses) << 20);
    if decodes
        .iter()
        .chain([&ecam])
        .any(|&(base, size)| base < bar + EHCI_DEBUG_BAR_SIZE && bar < base.saturating_add(size))
    {
        return None;
    }
    Some(Config {
        ecam_base,
        bus: 0,
        device: devfn >> 3,
        function: devfn & 7,
        bar: EHCI_DEBUG_BAR,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_choice_overrides_the_ehci2_default() {
        let both = |_| true;
        let only_ehci1 = |c| c == EhciController::Ehci1;
        assert_eq!(
            EhciController::select(None, both),
            Some(EhciController::Ehci2)
        );
        assert_eq!(
            EhciController::select(None, only_ehci1),
            Some(EhciController::Ehci1)
        );
        assert_eq!(
            EhciController::select(Some(EhciController::Ehci1), both),
            Some(EhciController::Ehci1)
        );
        assert_eq!(
            EhciController::select(Some(EhciController::Ehci2), only_ehci1),
            None
        );
        assert_eq!(EhciController::select(None, |_| false), None);
    }

    #[test]
    fn debug_bar_must_not_overlap_a_chipset_decode() {
        let debug = config(0xd7, 0xe000_0000, 256, &[(0xfed1_c000, 0x4000)]).unwrap();
        assert_eq!((debug.device, debug.function), (0x1a, 7));
        assert!(config(0xd7, 0xe000_0000, 256, &[(0xfef0_0000, 0x4000)]).is_none());
        assert!(config(0xd7, 0xf000_0000, 256, &[]).is_none());
        // Sandy Bridge's 64 buses at 0xf0000000 end below the BAR.
        assert!(config(0xd0, 0xf000_0000, 64, &[]).is_some());
    }
}
