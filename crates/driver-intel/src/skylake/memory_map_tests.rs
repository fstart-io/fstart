// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::super::{
    registers::testing::image,
    spd::{
        Dimm,
        testing::{seal, sodimm},
    },
    uncore_prmrr::tests::request,
};
use super::*;
use crate::sunrise_point::pcr::testing::ecam;
use fstart_pci::PciAddress;
use std::{boxed::Box, vec::Vec};

const CONFIG: EcamConfig = EcamConfig {
    base: 0xe0000000,
    buses: 256,
};
const CAPS: Capabilities = Capabilities {
    capid_a: 0,
    capid_b: 8,
    capid_c: 0,
};
const POLICY: Policy = Policy {
    tseg_mib: 8,
    mmio_mib: 2048,
};
#[repr(C, align(4096))]
struct GdxcImage([core::cell::UnsafeCell<u32>; 1024]);
impl GdxcImage {
    fn new() -> Box<Self> {
        Box::new(Self([const { core::cell::UnsafeCell::new(0) }; 1024]))
    }
    fn regs(&self) -> &GdxcRegisters {
        unsafe { GdxcRegisters::at(self.0.as_ptr() as usize) }
    }
    fn set(&self, offset: usize, value: u32) {
        unsafe { self.0[offset / 4].get().write(value) }
    }
    fn snapshot(&self) -> Vec<u32> {
        self.0.iter().map(|c| unsafe { *c.get() }).collect()
    }
}
fn ready(sa: &EcamDevice) {
    sa.write32(0, 0x191f8086);
    sa.write32(8, 0x06000001);
    sa.write32(0x60, CONFIG.encoded_pciexbar().unwrap());
    sa.write32(0x48, MCHBAR_BASE as u32 | 1);
    sa.write32(0xe8, 8);
    sa.write16(GGC, 0x2c0);
    sa.write32(DEVEN, 1 << 4);
    for (o, v) in [
        (TOM, ADDRESS),
        (REMAP_BASE, ADDRESS),
        (MESEG_BASE, ADDRESS),
        (BGSM, MIB),
        (TOLUD, MIB),
    ] {
        pair(sa, o, v);
    }
}
fn pair(sa: &EcamDevice, o: u16, v: u64) {
    sa.write32(o, v as u32);
    sa.write32(o + 4, (v >> 32) as u32);
}
fn snapshot(sa: &EcamDevice) -> Vec<u32> {
    (0..0x1000).step_by(4).map(|o| sa.read32(o)).collect()
}
fn uma(size: u8) -> UmaRequest {
    UmaRequest::from_shadow(0x10000 | u32::from(size))
        .unwrap()
        .unwrap()
}
fn run(
    sa: &EcamDevice,
    mch: &MchRegisters,
    g: &GdxcRegisters,
    d: &Dimms<'_>,
    me: u8,
    size: u16,
) -> Result<ProgrammedMap, Error> {
    unsafe {
        program_cold(
            sa,
            mch,
            g,
            CONFIG,
            MemoryBootMode::Cold,
            d,
            CAPS,
            uma(me),
            POLICY,
            request(size),
        )
    }
}

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
            request(prmrr_mib),
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
            request(256),
        ),
        Err(Error::Prmrr(uncore_prmrr::Error::InvalidAddress))
    );
}

#[test]
fn memory_map_program_words_and_lock_idempotency() {
    let (pci, _storage) = ecam();
    let sa = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    let mut raw = sodimm();
    seal(&mut raw);
    let d = Dimm::decode(&raw).unwrap();
    assert_eq!(d.size_mib, 8192);
    let dimms = [[Some(d), None], [None, None]];
    for (me, size) in [(8u8, 0u16), (64, 64)] {
        ready(&sa);
        let m = image();
        m.registers().gdxc_bar.set(GDXC_BASE | 1);
        let g = GdxcImage::new();
        // Unused-GDXC path doesn't inspect or clear these words.
        for (offset, value) in [(0, u32::MAX), (0x18, 1), (0x24, 8), (0x28, 1)] {
            g.set(offset, value);
        }
        let mut expected_gdxc = g.snapshot();
        expected_gdxc[0x24 / 4] = 4;
        let programmed = run(&sa, m.registers(), g.regs(), &dimms, me, size).unwrap();
        assert_eq!(g.snapshot(), expected_gdxc);
        let p = programmed.plan();
        assert_eq!(
            u64::from(sa.read32(TOM)) | (u64::from(sa.read32(TOM + 4)) << 32),
            p.tom
        );
        assert_eq!(
            u64::from(sa.read32(TOUUD)) | (u64::from(sa.read32(TOUUD + 4)) << 32),
            p.touud
        );
        assert_eq!(u64::from(sa.read32(TOLUD)), p.tolud);
        assert_eq!(u64::from(sa.read32(BDSM)), p.bdsm);
        assert_eq!(u64::from(sa.read32(BGSM)), p.bgsm);
        assert_eq!(u64::from(sa.read32(TSEG)), p.tseg);
        // ME physical backing lies numerically inside logical reclaim, and
        // must NOT be cut out again from the CPU logical high-RAM window.
        let me_base = p.me_physical_base.unwrap();
        assert_eq!(me_base, p.remap_base);
        assert!(me_base < p.touud);
        assert_eq!(p.tom - u64::from(me) * MIB + (FOUR_GIB - p.tolud), p.touud);
        // Locks introduced by later MC-done are accepted only if all match.
        for o in [TOM, TOLUD, TOUUD, REMAP_BASE, REMAP_LIMIT, TSEG, BDSM, BGSM] {
            sa.write32(o, sa.read32(o) | 1);
        }
        sa.write32(MESEG_MASK, sa.read32(MESEG_MASK) | 0x400);
        sa.write16(GGC, sa.read16(GGC) | 1);
        sa.write32(DPR, 1);
        let before = snapshot(&sa);
        run(&sa, m.registers(), g.regs(), &dimms, me, size).unwrap();
        assert_eq!(snapshot(&sa), before);
        // Reset for the next independent cold fixture.
        for o in (0x70..=0xbc).step_by(4) {
            sa.write32(o, 0);
        }
    }
}

#[test]
fn memory_map_native_disabled_igd_requires_vga_disabled_without_discarding_stolen_sizes() {
    let (pci, _storage) = ecam();
    let sa = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    ready(&sa);
    sa.write32(DEVEN, 0);
    sa.write16(GGC, 0x2c2);
    let raw = sodimm();
    let dimms = [[Some(Dimm::decode(&raw).unwrap()), None], [None, None]];
    let m = image();
    m.registers().gdxc_bar.set(GDXC_BASE | 1);
    let g = GdxcImage::new();
    let programmed = run(&sa, m.registers(), g.regs(), &dimms, 8, 0).unwrap();
    assert_eq!(programmed.plan().tolud - programmed.plan().bdsm, 64 * MIB);
}

#[test]
fn memory_map_native_checked_underflow_alignment_and_mib_granularity() {
    assert_eq!(
        compute(64, 0, 512, 8, POLICY, request(0)),
        Err(Error::Arithmetic)
    );
    assert_eq!(
        compute(8, 64, 0, 0, POLICY, request(0)),
        Err(Error::InvalidUma)
    );
    assert_eq!(
        compute(8193, 64, 0, 0, POLICY, request(0)),
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
        request(0),
    )
    .unwrap();
    assert_eq!(p.tolud, 2047 * MIB); // documented 1MiB, NOT unused C 4MiB mask
    for total in [1024, 2048, 4096, 8192, 32768, 65536] {
        for me in [0, 8, 64] {
            for size in [0, 1, 32, 64, 128] {
                if let Ok(p) = compute(total, me, 64, 8, POLICY, request(size)) {
                    assert!(p.prmrr.base() + u64::from(size) * MIB <= p.tseg || size == 0);
                    assert!(p.tseg <= p.bgsm && p.bgsm <= p.bdsm && p.bdsm <= p.tolud);
                    assert!(p.touud >= FOUR_GIB);
                    assert!(p.tolud <= 2048 * MIB);
                }
            }
        }
    }
}
