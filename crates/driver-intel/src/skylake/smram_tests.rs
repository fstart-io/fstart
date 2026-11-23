// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use crate::{
    skylake::{
        capabilities::Capabilities,
        cse::UmaRequest,
        early_sa::{EcamConfig, GDXC_BASE},
        memory_map::{self, Policy},
        registers::{GdxcRegisters, MCHBAR_BASE, testing::image},
        spd::{Dimm, testing::sodimm},
        uncore_prmrr::tests::request,
    },
    sunrise_point::pcr::testing::ecam,
};
use fstart_pci::PciAddress;
use std::vec::Vec;
use tock_registers::interfaces::{Readable, Writeable};

fn snapshot(sa: &EcamDevice) -> Vec<u32> {
    (0..0x1000).step_by(4).map(|o| sa.read32(o)).collect()
}
fn programmed_map(sa: &EcamDevice, tseg_mib: u32) -> ProgrammedMap {
    // Run the real map producer against canonical host images, not a fabricated
    // ProgrammedMap. Activation/done below are real; training is NOT exercised.
    let config = EcamConfig {
        base: 0xe0000000,
        buses: 256,
    };
    sa.write32(0, 0x191f8086);
    sa.write32(8, 0x06000001);
    sa.write32(0x60, config.encoded_pciexbar().unwrap());
    sa.write32(0x48, MCHBAR_BASE as u32 | 1);
    sa.write32(0xe8, 8);
    sa.write16(GGC, 0x2c0);
    sa.write32(DEVEN, 1 << 4);
    let address = MemoryBoundary::ADDRESS_MIB.mask << MemoryBoundary::ADDRESS_MIB.shift;
    for (offset, value) in [
        (TOM, address),
        (REMAP_BASE, address),
        (MESEG_BASE, address),
        (BGSM, 1 << 20),
        (TOLUD, 1 << 20),
    ] {
        sa.write32(offset, value as u32);
        sa.write32(offset + 4, (value >> 32) as u32);
    }
    sa.write8(SMRAMC, 2);
    let raw = sodimm();
    let dimms = [[Some(Dimm::decode(&raw).unwrap()), None], [None; 2]];
    let mch = image();
    mch.registers().gdxc_bar.set(GDXC_BASE | 1);
    let gdxc_image = image();
    // SAFETY: existing aligned interior-mutable register image is oversized.
    let gdxc = unsafe { GdxcRegisters::at(gdxc_image.0.as_ptr() as usize) };
    let map = unsafe {
        memory_map::program_cold(
            sa,
            mch.registers(),
            gdxc,
            config,
            MemoryBootMode::Cold,
            &dimms,
            Capabilities {
                capid_a: 0,
                capid_b: 8,
                capid_c: 0,
            },
            UmaRequest::from_shadow(0x10000).unwrap().unwrap(),
            Policy {
                tseg_mib,
                mmio_mib: 2048,
            },
            request(0),
        )
    }
    .unwrap();
    // Disclosed synthetic calibration/keys: exercise REAL activation and done,
    // not train_cold or a claim of trained DRAM. In particular never forge TSEG lock.
    use crate::skylake::{
        activation::{ActivationPolicy, PowerDown},
        cold_training::tests::input,
        reut::Engine,
        timings::{ReferenceClock, select_clock},
        training_state::CalibrationState,
        uncore_prmrr::tests::Cpu,
    };
    struct Ack<'a>(&'a crate::skylake::registers::MchRegisters);
    impl embedded_hal::delay::DelayNs for Ack<'_> {
        fn delay_ns(&mut self, _: u32) {
            if self.0.mc_lock.get() == 0x8f {
                self.0.init_state.set(self.0.init_state.get() | (1 << 5));
            }
        }
    }
    let e = Engine {
        registers: mch.registers(),
        dimms: &dimms,
    };
    mch.set(0x5e04, 7 | (1 << 8));
    let p = input(0x906e9);
    let mut delay = Ack(e.registers);
    unsafe {
        e.meminit_mc_activate(
            &CalibrationState::default(),
            MemoryBootMode::Cold,
            select_clock(1_000_000, ReferenceClock::Mhz133).unwrap(),
            ActivationPolicy {
                package: p.cpu.package,
                power_down: PowerDown::Auto,
                thermal: p.thermal,
                ecc: false,
                sa_gv: false,
                turnaround_trained: false,
                scrambling_keys: [0x1234, 0xabcd],
            },
            &mut delay,
        )
    }
    .unwrap();
    unsafe {
        e.meminit_mc_done(
            MemoryBootMode::Cold,
            false,
            sa,
            &map,
            &Cpu::new(),
            &mut delay,
        )
    }
    .unwrap();
    assert_eq!(sa.read32(TSEG) & 1, 0);
    map
}
fn only_control_changed(before: &[u32], sa: &EcamDevice, value: u8) {
    let mut expected = before.to_vec();
    expected[SMRAMC as usize / 4] = (expected[SMRAMC as usize / 4] & !255) | u32::from(value);
    assert_eq!(snapshot(sa), expected);
}

#[test]
fn smram_real_activation_done_enable_then_later_tseg_and_compatible_lock() {
    let (pci, _backing) = ecam();
    let sa = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    let map = programmed_map(&sa, 8);
    sa.write8(0x87, 0x95);
    sa.write8(0x89, 0xab);
    for initial in [2, 0x22, 0x42] {
        sa.write8(SMRAMC, initial);
        let before = snapshot(&sa);
        // SAFETY: exclusively owned device-model endpoints; no physical proof.
        let owner = unsafe { Smram::enable_cold(&sa, &map, MemoryBootMode::Cold) }.unwrap();
        only_control_changed(&before, &sa, initial | 8);
        assert_eq!(owner.tseg(), Some((map.plan().tseg, 8 << 20)));
        assert!(owner.smram_open());
        only_control_changed(&before, &sa, 0x4a);
        owner.smram_close();
        only_control_changed(&before, &sa, 0x0a);
        assert!(owner.smram_open());
        assert!(owner.smram_lock());
        let mut lock_expected = before.clone();
        lock_expected[TSEG as usize / 4] |= 1;
        only_control_changed(&lock_expected, &sa, 0x1a);
        assert_eq!(sa.read32(TSEG), map.plan().tseg as u32 | 1);
        let locked = snapshot(&sa);
        assert!(!owner.smram_open());
        owner.smram_close();
        assert!(owner.smram_lock());
        assert_eq!(snapshot(&sa), locked);
    }
}

#[test]
fn smram_operations_revalidate_map_decoder_and_fail_stop_void_close() {
    let (pci, _backing) = ecam();
    let sa = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    let map = programmed_map(&sa, 8);
    let owner = unsafe { Smram::enable_cold(&sa, &map, MemoryBootMode::Cold) }.unwrap();
    for (offset, mask) in [
        (SMRAMC, 8),
        (SMRAMC, 0x80),
        (TSEG, 1 << 20),
        (BGSM, 1),
        (TOLUD, 1),
    ] {
        let old = sa.read32(offset);
        sa.write32(offset, old ^ mask);
        let before = snapshot(&sa);
        assert!(!owner.smram_open());
        assert!(!owner.smram_lock());
        assert!(owner.close().is_err());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.smram_close())).is_err()
        );
        assert_eq!(snapshot(&sa), before);
        sa.write32(offset, old);
    }
    // Locked CLS=1 cannot be repaired and must not certify an operable handler.
    sa.write8(SMRAMC, 0x3a);
    let before = snapshot(&sa);
    assert_eq!(owner.close(), Err(Error::Locked));
    assert!(!owner.smram_lock());
    assert_eq!(snapshot(&sa), before);
}
