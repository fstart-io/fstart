// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h policy for the architectural MP engine: the MSR image APs
//! replay before enabling caches, and (Picasso) AP release through the SMU.
#[cfg(feature = "picasso")]
use fstart_driver_amd::fam17h::tsc::DelayNs;
use fstart_arch::x86::{
    cpu::{
        amd::msr::{SYSCFG, Syscfg},
        msr::MsrAccess,
    },
    mp::SipiUnlock,
    mtrr::{self, SipiMsrs},
};
use fstart_driver_amd::fam17h::cache;
#[cfg(feature = "picasso")]
use fstart_driver_amd::picasso::{
    registers::Smn,
    topology::{self, Topology},
};
use tock_registers::interfaces::ReadWriteable;

#[derive(Debug)]
pub enum Error {
    AmdCache(cache::Error),
    Capacity,
}

/// SYSCFG.MtrrFixDramModEn while an AP replays [`sipi_msrs`]: the fixed
/// MTRRs' RdDram/WrDram bits are only writable with it.
pub const SIPI_UNLOCK: SipiUnlock = SipiUnlock::new(SYSCFG, Syscfg::MTRR_FIX_DRAM_MOD_EN::SET);

/// BSP only, after the AMD cache plan is installed and before AP release:
/// the MTRRs and PAT the APs replay, the fixed ones read with their
/// RdDram/WrDram routing exposed.
///
/// The APs keep the SYSCFG, TOP_MEM and
/// TOM2 the ABL programmed on every core: a fresh core faults on the BSP's
/// SYSCFG. They replay with [`SIPI_UNLOCK`].
pub fn sipi_msrs(cpu: &impl MsrAccess) -> Result<SipiMsrs, Error> {
    cache::fixed_dram_view(cpu, |cpu| mtrr::snapshot_with(|index| cpu.read(index)))
        .map_err(Error::AmdCache)?
        .map_err(|_| Error::Capacity)
}

/// Hide the fixed MTRRs' RdDram/WrDram bits again, on every CPU after its
/// init: the ABL and the early cache setup leave them exposed, the OS
/// expects them hidden.
pub fn hide_fixed_dram_routing(cpu: &impl MsrAccess) {
    cpu.reg(SYSCFG).modify(Syscfg::MTRR_FIX_DRAM_MOD_EN::CLEAR);
}

#[cfg(feature = "picasso")]
/// Release `ap_count` APs through SMU ThreadEn. Released APs start at the
/// reset vector, where `picasso.S` sends them to the SIPI trampoline.
/// # Safety
/// Cold boot, all APs still held, `topology` is the applied topology, and the
/// SIPI trampoline is installed. Exclusive SMN access.
pub unsafe fn release_aps(
    smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
    topology: Topology,
    ap_count: u16,
    delay: &mut impl DelayNs,
) -> Result<(), topology::Error> {
    unsafe { topology::launch(smn, topology, usize::from(ap_count), delay) }.map(|_| ())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::msr::testing::MsrModel;
    const SYSCFG: u32 = super::SYSCFG.index();
    struct Cpu {
        syscfg: u64,
        writes: std::vec::Vec<(u32, u64)>,
        cap: u64,
    }
    impl MsrModel for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            match index {
                SYSCFG => self.syscfg,
                0xfe => self.cap,
                0x2ff => 0xc00,
                0x277 => 0x0007040600070406,
                0x250 | 0x258 | 0x259 | 0x268..=0x26f => {
                    assert_ne!(self.syscfg & (1 << 19), 0, "RdDram/WrDram must be exposed");
                    if index == 0x259 {
                        0
                    } else {
                        0x1e1e1e1e1e1e1e1e
                    }
                }
                _ => 0,
            }
        }
        fn write(&mut self, index: u32, value: u64) {
            assert_eq!(index, SYSCFG);
            self.syscfg = value;
            self.writes.push((index, value));
        }
    }
    #[test]
    fn replays_the_mtrrs_and_pat_read_with_the_amd_routing() {
        let original = (1 << 18) | (1 << 20) | (1 << 21) | (1 << 22);
        let cpu = RefCell::new(Cpu {
            syscfg: original,
            writes: std::vec::Vec::new(),
            cap: 0x108,
        });
        let msrs = sipi_msrs(&cpu).unwrap();
        assert_eq!(cpu.borrow().syscfg, original);
        let writes: std::vec::Vec<_> = msrs.iter().map(|w| (w.index, w.value())).collect();
        assert_eq!(writes[0], (0x250, 0x1e1e1e1e1e1e1e1e));
        assert!(writes.contains(&(0x259, 0)));
        assert!(!writes.iter().any(|&(index, _)| index == SYSCFG));
        assert_eq!(
            &writes[writes.len() - 2..],
            &[(0x277, 0x0007040600070406), (0x2ff, 0xc00)]
        );
        assert_eq!(
            SIPI_UNLOCK,
            SipiUnlock::new(super::SYSCFG, Syscfg::MTRR_FIX_DRAM_MOD_EN::SET)
        );
    }
    #[test]
    fn encrypted_or_missing_fixed_state_has_no_bsp_writes() {
        for (syscfg, cap) in [((1 << 23) | (1 << 21), 0x108), (1 << 21, 8)] {
            let cpu = RefCell::new(Cpu {
                syscfg,
                cap,
                writes: std::vec::Vec::new(),
            });
            assert!(matches!(sipi_msrs(&cpu), Err(Error::AmdCache(_))));
            assert!(cpu.borrow().writes.is_empty());
        }
    }
}
