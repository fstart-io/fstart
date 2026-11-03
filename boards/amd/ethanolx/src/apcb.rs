// SPDX-License-Identifier: MIT OR Apache-2.0
//! EthanolX APCB facts (Milan). Against the other Milan donor
//! (S8030), this board's APCB differs in exactly the wiring below
//! and five setup tokens: memory power-down on, ABL console on the
//! FCH UART1 MMIO (the board's early console, see lib.rs), no ROM3
//! window, PCIe reset pin scheme 2, and the 3DS sensor threshold.

use fstart_driver_amd::apcb::tables;
use fstart_driver_amd::apcb::{
    AblConsole, AblSerialPort, ApcbFacts, BoardId, ChannelWiring, MemoryPolicy, Policy, SpdSlot,
};
use fstart_driver_amd::sp3::soc::Generation;

/// SPD EEPROMs of both sockets behind the mux at 0x94; the mux
/// channel value is the control bitmask the vendor APCB carries
/// (128/64 for socket 0's halves, 32/16 for socket 1's), control
/// byte 3.
static SPD: &[SpdSlot] = &[
    SpdSlot::muxed(0, 0, 0, 0xa0, 0x94, 128, 3),
    SpdSlot::muxed(0, 0, 1, 0xa2, 0x94, 128, 3),
    SpdSlot::muxed(0, 1, 0, 0xa4, 0x94, 128, 3),
    SpdSlot::muxed(0, 1, 1, 0xa6, 0x94, 128, 3),
    SpdSlot::muxed(0, 2, 0, 0xa8, 0x94, 128, 3),
    SpdSlot::muxed(0, 2, 1, 0xaa, 0x94, 128, 3),
    SpdSlot::muxed(0, 3, 0, 0xac, 0x94, 128, 3),
    SpdSlot::muxed(0, 3, 1, 0xae, 0x94, 128, 3),
    SpdSlot::muxed(0, 4, 0, 0xa0, 0x94, 64, 3),
    SpdSlot::muxed(0, 4, 1, 0xa2, 0x94, 64, 3),
    SpdSlot::muxed(0, 5, 0, 0xa4, 0x94, 64, 3),
    SpdSlot::muxed(0, 5, 1, 0xa6, 0x94, 64, 3),
    SpdSlot::muxed(0, 6, 0, 0xa8, 0x94, 64, 3),
    SpdSlot::muxed(0, 6, 1, 0xaa, 0x94, 64, 3),
    SpdSlot::muxed(0, 7, 0, 0xac, 0x94, 64, 3),
    SpdSlot::muxed(0, 7, 1, 0xae, 0x94, 64, 3),
    SpdSlot::muxed(1, 0, 0, 0xa0, 0x94, 32, 3),
    SpdSlot::muxed(1, 0, 1, 0xa2, 0x94, 32, 3),
    SpdSlot::muxed(1, 1, 0, 0xa4, 0x94, 32, 3),
    SpdSlot::muxed(1, 1, 1, 0xa6, 0x94, 32, 3),
    SpdSlot::muxed(1, 2, 0, 0xa8, 0x94, 32, 3),
    SpdSlot::muxed(1, 2, 1, 0xaa, 0x94, 32, 3),
    SpdSlot::muxed(1, 3, 0, 0xac, 0x94, 32, 3),
    SpdSlot::muxed(1, 3, 1, 0xae, 0x94, 32, 3),
    SpdSlot::muxed(1, 4, 0, 0xa0, 0x94, 16, 3),
    SpdSlot::muxed(1, 4, 1, 0xa2, 0x94, 16, 3),
    SpdSlot::muxed(1, 5, 0, 0xa4, 0x94, 16, 3),
    SpdSlot::muxed(1, 5, 1, 0xa6, 0x94, 16, 3),
    SpdSlot::muxed(1, 6, 0, 0xa8, 0x94, 16, 3),
    SpdSlot::muxed(1, 6, 1, 0xaa, 0x94, 16, 3),
    SpdSlot::muxed(1, 7, 0, 0xac, 0x94, 16, 3),
    SpdSlot::muxed(1, 7, 1, 0xae, 0x94, 16, 3),
];

pub fn facts(generation: Generation) -> ApcbFacts {
    ApcbFacts {
        policy: Policy {
            mem: MemoryPolicy {
                power_down: Some(true),
                ..MemoryPolicy::DEFAULT
            },
            abl_serial_port: Some(AblSerialPort::Uart1Mmio),
            ..Policy::DEFAULT
        },
        console: AblConsole::default(),
        spd: SPD,
        // The vendor APCB has no board-ID structure.
        board_id: BoardId::None,
        wiring: ChannelWiring {
            memclk: None,
            cke: None,
            odt: None,
            cs: None,
            dimms_per_channel: 1,
            channels_per_socket: 8,
        },
        electrical: tables::electrical_reference(generation),
        freq: tables::freq_reference(generation),
        instances: &[],
        rom3_base_high: 0,
        pcie_reset_pin_select: 2,
        sensor_3ds_upper: 66,
    }
}

#[cfg(test)]
mod donor_tests {
    //! Generated-vs-donor APCB equivalence; see the S8030 test of
    //! the same name for the setup. Donor payloads are
    //! `ethanolx-milan-{0..4}.bin` in slot order 68/0, 68/8, 68/9,
    //! 60/0, 60/1.

    use super::facts;
    use fstart_driver_amd::sp3::soc::Generation;
    use fstart_platform_sp3::host::apcb::{Selector, apcbs, semantic_diff};

    fn allowed(diff: &str) -> bool {
        // The donor's active CBS has a low-priority dword entry with
        // MemClockValue/MemUserTimingMode; both already appear in
        // the main payload at Normal priority with the same values.
        if diff.contains("prio=[\"low\"]")
            && (diff.contains("MemClockValue") || diff.contains("MemUserTimingMode"))
        {
            return true;
        }
        // One unnamed token differs in value between the donor's
        // backup and active CBS payloads; we generate the backup's.
        if diff.contains("raw3006347150") {
            return true;
        }
        false
    }

    #[test]
    fn generated_apcbs_match_the_donor() {
        let Ok(dir) = std::env::var("FSTART_APCB_DONORS") else {
            eprintln!("skipping: FSTART_APCB_DONORS unset");
            return;
        };
        let generation = Generation::Milan;
        let generated = apcbs(generation, &facts(generation)).unwrap();
        let slots: [(u8, u8); 5] = [(0x68, 0), (0x68, 8), (0x68, 9), (0x60, 0), (0x60, 1)];
        for (i, &(kind, instance)) in slots.iter().enumerate() {
            let donor = std::fs::read(format!("{dir}/ethanolx-milan-{i}.bin"))
                .unwrap_or_else(|e| panic!("donor payload {i}: {e}"));
            let payload = generated
                .iter()
                .find(|g| {
                    g.selector
                        == Selector {
                            kind,
                            instance,
                            subprogram: 1,
                        }
                })
                .expect("slot generated");
            let unexpected: Vec<String> = semantic_diff(&donor, &payload.bytes)
                .into_iter()
                .filter(|d| !allowed(d))
                .collect();
            assert!(
                unexpected.is_empty(),
                "Milan slot {kind:#x}/{instance} unexpected diffs:\n{}",
                unexpected.join("\n")
            );
        }
    }
}
