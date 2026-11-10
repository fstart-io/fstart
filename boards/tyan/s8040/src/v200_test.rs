// SPDX-License-Identifier: MIT OR Apache-2.0
//! The MPIO requests the vendor firmware (GM2NE V2.00) builds from its
//! default connector table, as an independent model of its code decodes
//! them. Synthetic four-root lane map only: not the live fused roots or BDFs.
use crate::mpio::{self, Detected, Options};
use fstart_driver_amd::mpio::rs;

// (lane endpoints, root index, wrapper, ASK dword0, requested devfn, link).
const GOLDEN: &[(u8, u8, u8, u8, u32, u32, u32)] = &[
    (0, 3, 0, 0, 0x0004_0000, 0x0c, 1),
    (4, 7, 0, 0, 0x0004_0004, 0x0b, 1),
    (8, 11, 0, 0, 0x0004_0008, 0x0a, 1),
    (12, 15, 0, 0, 0x0004_000c, 0x09, 1),
    (16, 31, 3, 0, 0x0050_0010, 0, 1),
    (48, 63, 2, 0, 0x0050_0030, 0, 1),
    (96, 111, 0, 1, 0x0050_0060, 0, 1),
    (80, 83, 3, 1, 0x0004_0050, 0x19, 1),
    (84, 87, 3, 1, 0x0004_0054, 0x1a, 1),
    (88, 89, 3, 1, 0x0002_0058, 0x1b, 1),
    (90, 91, 3, 1, 0x0002_005a, 0x1c, 1),
    (92, 95, 3, 1, 0x1004_005c, 0, 1),
    (112, 127, 2, 1, 0x0010_0070, 0, 1),
    (132, 132, 0, 2, 0x0001_0084, 0, 0x2001),
    (133, 133, 0, 2, 0x0001_0085, 0, 0x2001),
    (134, 134, 0, 2, 0x0001_0086, 0, 0x2001),
];

#[test]
fn every_active_default_engine_matches_all_52_vendor_model_bytes() {
    // Deliberately supplied true to compare the un-gated vendor board table.
    // The board unit tests separately exercise missing BMC-link evidence.
    let engines = mpio::engines(
        &Options::DEFAULT,
        &Detected {
            bmc_link_present: true,
        },
    )
    .unwrap();
    assert_eq!(engines.len(), GOLDEN.len());
    for (e, &(start, end, root, wrapper, lane, devfn, link)) in engines.iter().zip(GOLDEN) {
        assert_eq!((e.port.start, e.port.end), (start, end));
        let (mapping, ask) = rs::build(&[0, 1, 2, 3], &e.port, 0).unwrap();
        assert_eq!((mapping.root_index, mapping.wrapper), (root, wrapper));
        let expected = [lane, 1, devfn, link, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let bytes: Vec<_> = expected.into_iter().flat_map(u32::to_le_bytes).collect();
        assert_eq!(ask.bytes(), bytes, "lane {start}..{end}");
    }
}
