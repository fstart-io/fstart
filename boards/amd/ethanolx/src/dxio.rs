//! RXM100CB reference lane map, unbifurcated bring-up configuration.
//!
//! AmdCpmOemInitPeim (7b8f8199-42c7-db87-307e-2892f3849538):
//! `$A26` at 0xffd68d74, `$A27` at 0xffd6bb1c, stride 0x1b0.
//! The vendor also patches these from board straps and PCIe presence;
//! those runtime options are outside this initial 2P bring-up port.

use fstart_driver_amd::sp3::dxio::engine::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::lanes::{Engine, Group, Lanes, PcieEngine, SataEngine, Speed};

// Socket 1 SATA descriptor +0x58: the vendor's terminated PHY parameter list.
const SATA_TUNING: &[(u8, u8)] = &[
    (0xc1, 0x00),
    (0xc2, 0x0e),
    (0xc0, 0x00),
    (0xd2, 0x01),
    (0xd3, 0x01),
    (0x81, 0x04),
    (0x82, 0x00),
    (0x80, 0x00),
    (0x92, 0x01),
    (0x93, 0x01),
    (0x41, 0x0a),
    (0x42, 0x00),
    (0x40, 0x00),
    (0x52, 0x01),
    (0x53, 0x01),
];

pub fn engines() -> heapless::Vec<Engine<'static>, MAX_ENGINES> {
    let port = |socket, group, device| {
        Engine::pcie(
            socket,
            Lanes::sp3(group, 0, 16),
            PcieEngine::new().devfn(device, 1).turn_off_unused(),
        )
    };
    [
        port(0, Group::P0, 3),
        port(0, Group::P1, 1),
        port(0, Group::P2, 1),
        port(0, Group::P3, 3),
        // PHY lane 129: the BMC's dedicated WAFL link, seen behind 60:05.2
        // under the vendor BIOS. Its fixed devfn comes from the silicon map.
        Engine::pcie(
            0,
            Lanes::sp3(Group::Wafl, 1, 1),
            PcieEngine::new().max_speed(Speed::Gen1).turn_off_unused(),
        ),
        Engine::sata(1, Lanes::sp3(Group::P1, 0, 8), SataEngine::new(0)).tuning(SATA_TUNING),
        port(1, Group::P3, 3),
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::sp3::dxio::lanes::{place, validate};
    use fstart_driver_amd::sp3::soc::Generation;

    #[test]
    fn reference_map_covers_both_sockets_and_the_bmc() {
        let engines = engines();
        validate(Generation::Milan, &engines).unwrap();
        assert_eq!(engines.iter().filter(|e| e.socket == 0).count(), 5);
        assert_eq!(engines.iter().filter(|e| e.socket == 1).count(), 2);
        let bmc = place(Generation::Milan, &engines[4]).unwrap();
        assert_eq!((bmc.nbio, bmc.phy), (0, (129, 129)));
    }
}
