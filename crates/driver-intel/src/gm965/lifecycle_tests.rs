//! Exercise the bootblock-to-ramstage transition over simulated ECAM/MMIO.
extern crate std;

use super::*;
use crate::IntelNorthbridgeDriver;
use alloc::boxed::Box;
use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};

/// Includes temporary bus 1 for the pre-DRAM PEG probe. No other driver-intel
/// test installs the global ECAM mapping; restore it even if an assertion fails.
struct Ecam {
    memory: *mut u8,
    layout: Layout,
    previous: usize,
}
impl Ecam {
    fn new() -> Self {
        let layout = Layout::from_size_align(2 << 20, 1 << 20).unwrap();
        // SAFETY: a nonzero valid layout, released by Drop.
        let memory = unsafe { alloc_zeroed(layout) };
        if memory.is_null() {
            handle_alloc_error(layout);
        }
        let previous = ecam::base();
        ecam::init(memory as usize);
        Self {
            memory,
            layout,
            previous,
        }
    }
}
impl Drop for Ecam {
    fn drop(&mut self) {
        ecam::init(self.previous);
        // SAFETY: the original allocation/layout, no users remain.
        unsafe { dealloc(self.memory, self.layout) };
    }
}

#[test]
fn ramstage_keeps_the_gpu_selection_and_memory_map_from_bootblock() {
    let ecam = Ecam::new();
    let mut mch = [0u32; 0x1000];
    let mut dmi = [0u32; 0x1000];
    let config = Box::leak(Box::new(IntelGm965Config {
        ecam_base: ecam.memory as u64,
        mchbar: mch.as_mut_ptr() as u64,
        dmibar: dmi.as_mut_ptr() as u64,
        enable_peg: true,
        ..IntelGm965Config::new()
    }));
    let igd = ecam::EcamDevice::new(0, 2, 0);
    let peg = ecam::EcamDevice::new(0, 1, 0);
    let gpu = ecam::EcamDevice::new(1, 0, 0);
    igd.write32(0, 0x2a02_8086);
    gpu.write32(0, 0x0429_10de);
    gpu.write32(8, 0x0300_0000);
    peg.write16(0xb2, 16 << 4);

    for discrete in [true, false] {
        peg.write16(0xba, if discrete { 1 << 6 } else { 0 });
        let mut bootblock = IntelGm965::new_from_config(config).unwrap();
        let hb = bootblock.hostbridge_regs();
        hb.ggc.set(0);
        hb.deven.set(0);
        bootblock.early_init().unwrap();
        assert_eq!(bootblock.peg_display, discrete);
        // IGD remains available to the pre-DRAM clock code on either model.
        assert!(bootblock.igd_visible());
        assert!(!bootblock.igd_enabled());

        // Raminit reserves memory from this selection, then clock setup ends.
        hb.ggc.set(bootblock.igd_ggc());
        hb.tolud.set(0x8000);
        hb.esmramc.set(1); // 1 MiB TSEG.
        igd.write32(hostbridge::IGD_BSM, if discrete { 0 } else { 0x7e00_0000 });
        bootblock.hide_unused_igd();
        let selected = hb.deven.get();
        let ggc = hb.ggc.get();
        let tseg = bootblock.tseg_base();

        // The new stage gets a fresh driver, but must not repeat detection:
        // an OS-owned GPU can have no visible slot presence while asleep.
        peg.write16(0xba, 0);
        let mut ramstage = IntelGm965::new_from_config(config).unwrap();
        ramstage.ramstage_early_init().unwrap();
        assert_eq!(hb.deven.get(), selected);
        assert_eq!(hb.ggc.get(), ggc);
        assert_eq!(ramstage.tseg_base(), tseg);
        assert_eq!(ramstage.igd_enabled(), !discrete);
        assert_eq!(ramstage.peg_display, discrete);
        if discrete {
            assert_eq!(ggc, 2);
            assert_eq!(selected, hostbridge::DEVEN_D0F0 | hostbridge::DEVEN_D1F0);
            assert_eq!(tseg, 0x7ff0_0000);
            ramstage
                .post_verify_init(&mut |_| panic!("PEG must not allocate an IGD OpRegion"))
                .unwrap();
            // A stale visible function must still never get a native GTT.
            hb.deven
                .set(selected | hostbridge::DEVEN_D2F0 | hostbridge::DEVEN_D2F1);
            assert!(!ramstage.igd_enabled());
            ramstage.ramstage_early_init().unwrap();
            assert_eq!(hb.deven.get(), selected);
        }
    }
}
