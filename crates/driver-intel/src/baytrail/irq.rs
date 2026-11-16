//! Interrupt routing: the legacy block routes each PCI device's INTA-D to
//! one of PIRQA-H (IOAPIC inputs 16-23), with the same 16-bit per-device
//! encoding as the ICH `DxxIR` registers.

use super::dev;
use crate::southbridge::pirq::{PciPin as P, PinRoute as R, Pirq as L, PirqRouting};

const fn identity(slot: u8) -> [R; 4] {
    [
        R::new(slot, P::A, L::A),
        R::new(slot, P::B, L::B),
        R::new(slot, P::C, L::C),
        R::new(slot, P::D, L::D),
    ]
}

const DEVICES: [u8; 16] = [
    dev::GFX,
    dev::MIPI,
    dev::MMC,
    dev::SDIO,
    dev::SD,
    dev::SATA,
    dev::XHCI,
    dev::LPE,
    dev::MMC45,
    dev::SIO1,
    dev::TXE,
    dev::HDA,
    dev::PCIE,
    dev::EHCI,
    dev::SIO2,
    dev::PCU,
];

const IDENTITY_ROUTES: [R; 64] = {
    let mut routes = [R::new(0, P::A, L::A); 64];
    let mut i = 0;
    while i < DEVICES.len() {
        let four = identity(DEVICES[i]);
        let mut pin = 0;
        while pin < 4 {
            routes[i * 4 + pin] = four[pin];
            pin += 1;
        }
        i += 1;
    }
    routes
};

/// Every device's INTA-D on PIRQA-D (coreboot's Bay Trail boards).
pub const IDENTITY_ROUTING: PirqRouting = PirqRouting {
    slot_pins: &[],
    pin_routes: &IDENTITY_ROUTES,
    bridge_routes: &[],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_routes_encode_like_the_reference_macro() {
        // PCI_DEV_PIRQ_ROUTE(dev, A, B, C, D) = D << 12 | C << 8 | B << 4 | A.
        assert_eq!(IDENTITY_ROUTING.ir_value(dev::SATA), 0x3210);
        assert_eq!(IDENTITY_ROUTING.ir_value(0x16), 0);
        assert_eq!(IDENTITY_ROUTING.gsi(dev::PCIE, P::C), 18);
    }
}
