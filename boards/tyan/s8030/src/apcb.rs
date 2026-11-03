// SPDX-License-Identifier: MIT OR Apache-2.0
//! S8030 APCB facts: what ABL needs from this board (see
//! `fstart_driver_amd::apcb` for the split). Values are the S8030
//! v4.04 donor's, transcribed where cited; policy stays at the AGESA
//! defaults except where noted.
//!
//! The donor's SPD table carries socket-1 rows although the S8030 is
//! 1P: the mux channels 6/7 they point at are the reference design's.
//! They are kept until ABL is proven on the bench to accept the short
//! table, so the first generated APCB differs from the donor only in
//! the ways the encoder documents.

use fstart_driver_amd::apcb::tables::{DdrRate, FreqRow, Slots};
use fstart_driver_amd::apcb::{
    AblConsole, ApcbFacts, BoardId, ChannelWiring, IdMapping, Instance, Policy, SpdSlot, XgmiSpeed,
};
use fstart_driver_amd::sp3::soc::Generation;

/// SPD EEPROMs behind the mux at 0xE0: channels 4/5 serve socket 0,
/// 6/7 are the vendor table's socket-1 rows (see the module docs).
static SPD: &[SpdSlot] = &[
    SpdSlot::muxed(0, 0, 0, 0xa0, 0xe0, 4, 0),
    SpdSlot::muxed(0, 0, 1, 0xa2, 0xe0, 4, 0),
    SpdSlot::muxed(0, 1, 0, 0xa4, 0xe0, 4, 0),
    SpdSlot::muxed(0, 1, 1, 0xa6, 0xe0, 4, 0),
    SpdSlot::muxed(0, 2, 0, 0xa8, 0xe0, 4, 0),
    SpdSlot::muxed(0, 2, 1, 0xaa, 0xe0, 4, 0),
    SpdSlot::muxed(0, 3, 0, 0xac, 0xe0, 4, 0),
    SpdSlot::muxed(0, 3, 1, 0xae, 0xe0, 4, 0),
    SpdSlot::muxed(0, 4, 0, 0xa0, 0xe0, 5, 0),
    SpdSlot::muxed(0, 4, 1, 0xa2, 0xe0, 5, 0),
    SpdSlot::muxed(0, 5, 0, 0xa4, 0xe0, 5, 0),
    SpdSlot::muxed(0, 5, 1, 0xa6, 0xe0, 5, 0),
    SpdSlot::muxed(0, 6, 0, 0xa8, 0xe0, 5, 0),
    SpdSlot::muxed(0, 6, 1, 0xaa, 0xe0, 5, 0),
    SpdSlot::muxed(0, 7, 0, 0xac, 0xe0, 5, 0),
    SpdSlot::muxed(0, 7, 1, 0xae, 0xe0, 5, 0),
    SpdSlot::muxed(1, 0, 0, 0xa0, 0xe0, 6, 0),
    SpdSlot::muxed(1, 0, 1, 0xa2, 0xe0, 6, 0),
    SpdSlot::muxed(1, 1, 0, 0xa4, 0xe0, 6, 0),
    SpdSlot::muxed(1, 1, 1, 0xa6, 0xe0, 6, 0),
    SpdSlot::muxed(1, 2, 0, 0xa8, 0xe0, 6, 0),
    SpdSlot::muxed(1, 2, 1, 0xaa, 0xe0, 6, 0),
    SpdSlot::muxed(1, 3, 0, 0xac, 0xe0, 6, 0),
    SpdSlot::muxed(1, 3, 1, 0xae, 0xe0, 6, 0),
    SpdSlot::muxed(1, 4, 0, 0xa0, 0xe0, 7, 0),
    SpdSlot::muxed(1, 4, 1, 0xa2, 0xe0, 7, 0),
    SpdSlot::muxed(1, 5, 0, 0xa4, 0xe0, 7, 0),
    SpdSlot::muxed(1, 5, 1, 0xa6, 0xe0, 7, 0),
    SpdSlot::muxed(1, 6, 0, 0xa8, 0xe0, 7, 0),
    SpdSlot::muxed(1, 6, 1, 0xaa, 0xe0, 7, 0),
    SpdSlot::muxed(1, 7, 0, 0xac, 0xe0, 7, 0),
    SpdSlot::muxed(1, 7, 1, 0xae, 0xe0, 7, 0),
];

/// Board-ID EEPROM (I2C5, 0x50) to instance mapping of the Rome
/// donor. IDs are Tyan product codes; instance 1 is the derated
/// carrier variant, instance 2 has no APCB deltas and takes the
/// global entries.
static BOARD_ID_ROME: &[IdMapping] = &[
    IdMapping {
        mask: 255,
        id: 12,
        revision: None,
        instance: 1,
    },
    IdMapping {
        mask: 255,
        id: 55,
        revision: None,
        instance: 2,
    },
    IdMapping {
        mask: 255,
        id: 13,
        revision: None,
        instance: 0,
    },
    IdMapping {
        mask: 255,
        id: 49,
        revision: None,
        instance: 0,
    },
    IdMapping {
        mask: 255,
        id: 57,
        revision: None,
        instance: 1,
    },
    IdMapping {
        mask: 255,
        id: 48,
        revision: None,
        instance: 0,
    },
    IdMapping {
        mask: 255,
        id: 52,
        revision: Some(5),
        instance: 1,
    },
    IdMapping {
        mask: 255,
        id: 52,
        revision: None,
        instance: 0,
    },
    IdMapping {
        mask: 255,
        id: 14,
        revision: None,
        instance: 0,
    },
];

/// The Milan donor's shorter mapping table.
static BOARD_ID_MILAN: &[IdMapping] = &[
    IdMapping {
        mask: 255,
        id: 13,
        revision: None,
        instance: 0,
    },
    IdMapping {
        mask: 255,
        id: 48,
        revision: None,
        instance: 0,
    },
    IdMapping {
        mask: 255,
        id: 52,
        revision: Some(5),
        instance: 1,
    },
    IdMapping {
        mask: 255,
        id: 52,
        revision: Some(6),
        instance: 1,
    },
    IdMapping {
        mask: 255,
        id: 52,
        revision: Some(7),
        instance: 1,
    },
    IdMapping {
        mask: 255,
        id: 52,
        revision: None,
        instance: 0,
    },
];

/// Speed limits of Rome board instance 1 (the donor's masked
/// MaxFreq entries): capped at 2667 MT/s 1DPC, 2400 2DPC 1R/1xDR.
static RDIMM_MAX_INSTANCE1: &[FreqRow] = &[
    FreqRow {
        slots: Slots::ONE,
        dimms: 1,
        single_rank: 1,
        dual_rank: 0,
        quad_rank: 0,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::ONE,
        dimms: 1,
        single_rank: 0,
        dual_rank: 1,
        quad_rank: 0,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 1,
        single_rank: 1,
        dual_rank: 0,
        quad_rank: 0,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 1,
        single_rank: 0,
        dual_rank: 1,
        quad_rank: 0,
        max: DdrRate::Ddr2400,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 2,
        single_rank: 2,
        dual_rank: 0,
        quad_rank: 0,
        max: DdrRate::Ddr2133,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 2,
        single_rank: 1,
        dual_rank: 1,
        quad_rank: 0,
        max: DdrRate::Ddr2133,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 2,
        single_rank: 0,
        dual_rank: 2,
        quad_rank: 0,
        max: DdrRate::Ddr2133,
    },
];
static THREE_DS_MAX_INSTANCE1: &[FreqRow] = &[
    FreqRow {
        slots: Slots::ONE,
        dimms: 1,
        single_rank: 0,
        dual_rank: 1,
        quad_rank: 0,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 1,
        single_rank: 0,
        dual_rank: 1,
        quad_rank: 0,
        max: DdrRate::Ddr2400,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 2,
        single_rank: 0,
        dual_rank: 2,
        quad_rank: 0,
        max: DdrRate::Ddr1866,
    },
];
static LRDIMM_MAX_INSTANCE1: &[FreqRow] = &[
    FreqRow {
        slots: Slots::ONE,
        dimms: 1,
        single_rank: 0,
        dual_rank: 1,
        quad_rank: 0,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::ONE,
        dimms: 1,
        single_rank: 0,
        dual_rank: 0,
        quad_rank: 1,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 1,
        single_rank: 0,
        dual_rank: 1,
        quad_rank: 0,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 1,
        single_rank: 0,
        dual_rank: 0,
        quad_rank: 1,
        max: DdrRate::Ddr2667,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 2,
        single_rank: 0,
        dual_rank: 2,
        quad_rank: 0,
        max: DdrRate::Ddr2133,
    },
    FreqRow {
        slots: Slots::TWO,
        dimms: 2,
        single_rank: 0,
        dual_rank: 0,
        quad_rank: 2,
        max: DdrRate::Ddr2133,
    },
];

/// Rome board instance 1 (see `facts`): separate statics because
/// the fact slices are `&'static`.
static ROME_INSTANCES: &[Instance] = &[Instance {
    index: 1,
    policy: Policy {
        abl_console_out: Some(true),
        bmc_socket: Some(15),
        bmc_start_lane: Some(255),
        bmc_end_lane: Some(255),
        xgmi_4link_speed: Some(XgmiSpeed::Mbps10667),
        ..Policy::DEFAULT
    },
    freq: Some(fstart_driver_amd::apcb::tables::FreqTables {
        rdimm_max: RDIMM_MAX_INSTANCE1,
        three_ds_max: THREE_DS_MAX_INSTANCE1,
        lrdimm_max: LRDIMM_MAX_INSTANCE1,
        ..fstart_driver_amd::apcb::tables::freq_reference(Generation::Rome)
    }),
}];

pub fn facts(generation: Generation) -> ApcbFacts {
    use fstart_driver_amd::apcb::tables;
    let freq = tables::freq_reference(generation);
    let (wiring, board_id, instances): (ChannelWiring, BoardId, &[Instance]) = match generation {
        // The Rome donor spells out the (identity) pin maps the Milan
        // donor leaves to ABL defaults.
        Generation::Rome => (
            ChannelWiring {
                memclk: Some([0, 1, 2, 3, 0, 0, 0, 0]),
                cke: Some([0, 1, 2, 3]),
                odt: Some([0, 1, 2, 3]),
                cs: Some([0, 1, 2, 3, 0, 0, 0, 0]),
                dimms_per_channel: 1,
                channels_per_socket: 8,
            },
            BoardId::Eeprom {
                controller: 5,
                address: 80,
                id_offset: 16,
                revision_offset: 18,
                mapping: BOARD_ID_ROME,
            },
            ROME_INSTANCES,
        ),
        _ => (
            ChannelWiring {
                memclk: None,
                cke: None,
                odt: None,
                cs: None,
                dimms_per_channel: 1,
                channels_per_socket: 8,
            },
            BoardId::Eeprom {
                controller: 5,
                address: 80,
                id_offset: 16,
                revision_offset: 18,
                mapping: BOARD_ID_MILAN,
            },
            &[],
        ),
    };
    ApcbFacts {
        policy: Policy::default(),
        console: AblConsole::default(),
        spd: SPD,
        board_id,
        wiring,
        electrical: tables::electrical_reference(generation),
        freq,
        instances,
        // The S8030's ROM3 window base above 4 GiB (0x7fc), as the
        // vendor APCB has it on both generations.
        rom3_base_high: 2044,
        pcie_reset_pin_select: 0,
        sensor_3ds_upper: 75,
    }
}

#[cfg(test)]
mod donor_tests {
    //! Generated-vs-donor APCB equivalence. Runs when
    //! `FSTART_APCB_DONORS` names a directory of donor payloads
    //! (`s8030-{rome,milan}-{0..4}.bin` in slot order 68/0, 68/8,
    //! 68/9, 60/0, 60/1) extracted from the vendor image; skipped
    //! otherwise. Every allowed difference is enumerated in
    //! `allowed` with its reason; anything else fails.

    use super::facts;
    use fstart_driver_amd::sp3::soc::Generation;
    use fstart_platform_sp3::host::apcb::{Selector, apcbs, semantic_diff};

    fn allowed(diff: &str) -> bool {
        // The vendor tool writes a board-instance-0 bool entry that
        // is a byte copy of the global one (Milan: minus three
        // tokens the global also carries). The encoder emits masked
        // entries only where values differ.
        if diff.starts_with("g=12288 id=0 inst=0 mask=1") && diff.ends_with("only in donor") {
            return true;
        }
        // Rome instance 1's masked byte entry omits these six tokens;
        // ours carries them with the global (donor) values, which is
        // the same effective configuration if ABL falls back from a
        // masked entry to the global one. Flagged for the bench: if
        // ABL instead treats a present masked entry as complete,
        // instance 1 needs token *removal* support in the encoder.
        const SUBSET: [&str; 6] = [
            "MemDramDoubleRefreshRate",
            "MemOverrideDimmSpdMaxActivityCount",
            "MemSelfRefreshExitStaggering",
            "MemSubUrgRefLowerBound",
            "MemUrgRefLimit",
            "SecondPcieLinkSpeed",
        ];
        if diff.starts_with("g=12288 id=1 inst=0 mask=2")
            && SUBSET.iter().any(|t| diff.contains(t))
            && diff.contains("donor=null")
        {
            return true;
        }
        // The Milan CBS catalog is the subset both Milan donors
        // share; this board's donor CBS carries five more tokens
        // (its AMI setup package's defaults: NPS, memory
        // interleaving, ROM3 base and two unnamed). Their setup
        // values are already in the main payload.
        const CBS_EXTRA: [&str; 5] = [
            "DfDramNumaPerSocket",
            "DfMemInterleaving",
            "raw3649799737",
            "raw4103355309",
            "FchRom3BaseHigh",
        ];
        if diff.contains("prio=[\"medium\"]")
            && CBS_EXTRA.iter().any(|t| diff.contains(t))
            && diff.contains("gen=null")
        {
            return true;
        }
        // Milan's active CBS omits one unnamed token its backup (and
        // our single CBS token list) carries.
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
        for (generation, prefix) in [
            (Generation::Rome, "s8030-rome"),
            (Generation::Milan, "s8030-milan"),
        ] {
            let generated = apcbs(generation, &facts(generation)).unwrap();
            let slots: [(u8, u8); 5] = [(0x68, 0), (0x68, 8), (0x68, 9), (0x60, 0), (0x60, 1)];
            for (i, &(kind, instance)) in slots.iter().enumerate() {
                let donor = std::fs::read(format!("{dir}/{prefix}-{i}.bin"))
                    .unwrap_or_else(|e| panic!("donor payload {i}: {e}"));
                let payload = generated
                    .iter()
                    .find(|g| {
                        g.selector
                            == Selector {
                                kind,
                                instance,
                                subprogram: u8::from(generation == Generation::Milan),
                            }
                    })
                    .expect("slot generated");
                let unexpected: Vec<String> = semantic_diff(&donor, &payload.bytes)
                    .into_iter()
                    .filter(|d| !allowed(d))
                    .collect();
                assert!(
                    unexpected.is_empty(),
                    "{generation:?} slot {kind:#x}/{instance} unexpected diffs:\n{}",
                    unexpected.join("\n")
                );
            }
        }
    }
}
