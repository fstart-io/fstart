// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

const POLICY: Policy = Policy {
    tseg_mib: 8,
    mmio_mib: 2048,
};

#[test]
fn memory_map_computation_cases_and_prmrr_alignment() {
    // (dram_mib, uma, dsm, gsm, tseg, prmrr, mmio) -> (tolud, bdsm, bgsm, tseg, prmrr_base, tom, touud, remap_base, remap_limit_mib)
    let cases = [
        (
            1024, 0, 0, 0, 8, 0, 2048, 1024, 1024, 1024, 1016, 0, 1024, 4096, None,
        ),
        (
            2048, 8, 64, 8, 8, 0, 2048, 2040, 1976, 1968, 1960, 0, 2048, 4096, None,
        ),
        (
            4096,
            64,
            64,
            8,
            8,
            0,
            2048,
            2048,
            1984,
            1976,
            1968,
            0,
            4096,
            6080,
            Some((4096, 6079)),
        ),
        (
            8192,
            8,
            64,
            8,
            8,
            0,
            2048,
            2048,
            1984,
            1976,
            1968,
            0,
            8192,
            10232,
            Some((8184, 10231)),
        ),
        (
            32768,
            64,
            512,
            8,
            16,
            32,
            2048,
            1976,
            1464,
            1456,
            1440,
            1408,
            32768,
            34824,
            Some((32704, 34823)),
        ),
        (
            8192,
            64,
            64,
            8,
            8,
            64,
            2048,
            1936,
            1872,
            1864,
            1856,
            1792,
            8192,
            10288,
            Some((8128, 10287)),
        ),
        (
            4096,
            0,
            32,
            2,
            16,
            128,
            1024,
            2994,
            2962,
            2960,
            2944,
            2816,
            4096,
            5198,
            Some((4096, 5197)),
        ),
        (
            2048, 0, 0, 0, 0, 0, 512, 2048, 2048, 2048, 2048, 0, 2048, 4096, None,
        ),
    ];
    for (
        dram,
        uma_mib,
        dsm,
        gsm,
        tseg_mib,
        prmrr_mib,
        mmio_mib,
        tolud,
        bdsm,
        bgsm,
        tseg,
        prmrr_base,
        tom,
        touud,
        remap,
    ) in cases
    {
        let p = compute(
            dram,
            uma_mib,
            dsm,
            gsm,
            Policy { tseg_mib, mmio_mib },
            uncore_prmrr::Request::for_test(prmrr_mib),
        )
        .unwrap();
        assert_eq!(
            [p.tolud / MIB, p.bdsm / MIB, p.bgsm / MIB, p.tseg / MIB],
            [tolud, bdsm, bgsm, tseg]
        );
        if prmrr_mib != 0 {
            assert_eq!(p.prmrr.base() / MIB, prmrr_base);
        }
        assert_eq!(p.tom, tom * MIB);
        assert_eq!(p.touud, touud * MIB);
        if let Some((base_mib, limit_mib)) = remap {
            assert_eq!(p.remap_base, base_mib * MIB);
            assert_eq!(p.remap_limit, limit_mib * MIB);
        } else {
            assert_eq!((p.remap_base, p.remap_limit), (ADDRESS, 0));
        }
    }
    assert_eq!(
        compute(
            8192,
            64,
            64,
            8,
            Policy {
                tseg_mib: 8,
                mmio_mib: 2048,
            },
            uncore_prmrr::Request::for_test(256),
        ),
        Err(Error::Prmrr(uncore_prmrr::Error::InvalidAddress))
    );
}

#[test]
fn memory_map_native_checked_underflow_alignment_and_mib_granularity() {
    assert_eq!(
        compute(64, 0, 512, 8, POLICY, uncore_prmrr::Request::for_test(0)),
        Err(Error::Arithmetic)
    );
    assert_eq!(
        compute(8, 64, 0, 0, POLICY, uncore_prmrr::Request::for_test(0)),
        Err(Error::InvalidUma)
    );
    assert_eq!(
        compute(8193, 64, 0, 0, POLICY, uncore_prmrr::Request::for_test(0)),
        Err(Error::InvalidUma)
    );
    let p = compute(
        8192,
        8,
        64,
        8,
        Policy {
            tseg_mib: 0,
            mmio_mib: 2049,
        },
        uncore_prmrr::Request::for_test(0),
    )
    .unwrap();
    assert_eq!(p.tolud, 2047 * MIB); // documented 1MiB, NOT unused C 4MiB mask
    for total in [1024, 2048, 4096, 8192, 32768, 65536] {
        for me in [0, 8, 64] {
            for size in [0, 1, 32, 64, 128] {
                if let Ok(p) = compute(
                    total,
                    me,
                    64,
                    8,
                    POLICY,
                    uncore_prmrr::Request::for_test(size),
                ) {
                    assert!(p.prmrr.base() + u64::from(size) * MIB <= p.tseg || size == 0);
                    assert!(p.tseg <= p.bgsm && p.bgsm <= p.bdsm && p.bdsm <= p.tolud);
                    assert!(p.touud >= FOUR_GIB);
                    assert!(p.tolud <= 2048 * MIB);
                }
            }
        }
    }
}
