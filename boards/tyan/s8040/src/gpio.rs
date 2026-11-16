// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's pads: the OEM CPM GPIO table (`$A10`, not the generic
//! Sunstone one) and the MCIO connector-mode pads.
//! OemPei copies the table from `ffda8e5c` (36 rows and a pin 255
//! terminator); AmdCpmInitPeim remaps only the pins of a die other than 0,
//! so the pins below 256 are FCH 0's and 265 is pin 9 of the remote bank.
//! The signals' names are not known.

use fstart_driver_amd::fch::gpio::{Pad, Pin};

const fn cpm(pin: u16, setting: u16) -> Pad {
    Pad::from_cpm(Pin(pin), setting)
}

/// The OEM CPM table, then the connector-mode pads of the build's MCIO
/// choices ([`connector_mode`]), in vendor order.
pub const GPIO: [Pad; 39] = {
    let mode = connector_mode(&crate::mpio::Options::DEFAULT);
    [
        cpm(0, 0x888),
        cpm(1, 0x888),
        cpm(2, 0x888),
        cpm(3, 0x988),
        cpm(4, 0x988),
        cpm(5, 0x881),
        cpm(6, 0x881),
        cpm(7, 0x881),
        cpm(12, 0x888),
        cpm(13, 0x888),
        cpm(14, 0x888),
        cpm(16, 0x982),
        cpm(17, 0x982),
        cpm(19, 0x888),
        cpm(20, 0x888),
        cpm(24, 0x888),
        cpm(26, 0x888),
        cpm(76, 0x988),
        cpm(87, 0x882),
        cpm(88, 0x882),
        cpm(89, 0x988),
        cpm(104, 0x882),
        cpm(105, 0x882),
        cpm(107, 0x881),
        cpm(109, 0x882),
        cpm(115, 0x881),
        cpm(116, 0x881),
        cpm(126, 0x982),
        cpm(135, 0xa81),
        cpm(136, 0x888),
        cpm(137, 0xa81),
        cpm(138, 0x888),
        cpm(139, 0x982),
        cpm(141, 0x981),
        cpm(142, 0x981),
        cpm(265, 0x980),
        mode[0],
        mode[1],
        mode[2],
    ]
};

/// OemPei `ffb027b2`'s rows at `ffb038c4`: (setup byte, pin, CPM setting).
/// After the CPM table the vendor drives each MCIO connector-mode pad from
/// its connector's bifurcation: low (GPIO, pull-down) for every PCIe
/// choice, high (`| 0x41`) only for the hidden SATA templates 13 and 18.
/// Left as pulled-up inputs, MCIO0/1 and MCIO2/3 fail to train.
const CONNECTOR_MODE: [(usize, u16, u16); 3] = [(4, 21, 0x8c0), (4, 22, 0x8c0), (5, 106, 0x8c0)];

/// The connector-mode pads for `options`.
pub const fn connector_mode(options: &crate::mpio::Options) -> [Pad; 3] {
    let setup = options.slot_variable();
    let mut pads = [cpm(0, 0); 3];
    let mut i = 0;
    while i < pads.len() {
        let (offset, pin, setting) = CONNECTOR_MODE[i];
        let sata = matches!(setup[offset], 13 | 18);
        pads[i] = cpm(pin, if sata { setting | 0x41 } else { setting });
        i += 1;
    }
    pads
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_mode_pads_follow_the_mcio_setup_bytes() {
        let pcie = connector_mode(&crate::mpio::Options::DEFAULT);
        assert_eq!(pcie, [cpm(21, 0x8c0), cpm(22, 0x8c0), cpm(106, 0x8c0)]);
        // Only the hidden SATA templates drive a connector's pads high.
        assert!(
            CONNECTOR_MODE
                .iter()
                .all(|&(offset, ..)| offset == 4 || offset == 5)
        );
    }

    #[test]
    fn every_pad_once() {
        for (i, pad) in GPIO.iter().enumerate() {
            assert!(pad.pin.is_valid());
            assert!(!GPIO[..i].iter().any(|other| other.pin == pad.pin));
        }
    }
}
