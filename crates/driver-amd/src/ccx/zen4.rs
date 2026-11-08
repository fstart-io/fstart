// SPDX-License-Identifier: MIT OR Apache-2.0
//! Siena's per-CCD thread-enable register. S8040 V2.00 AmdSocSp5RsPei
//! `ffd24456`, sp5 `cpu.ap-thread-enable`. The live APOB map supplies
//! physical IDs; compiled dimensions below are bounds, NOT CPU counts.
//!
//! This primitive does not prepare AP code, relocate stacks, initialize
//! per-thread MSRs, switch x2APIC, or wait for successful AP rendezvous.

use crate::smn::{SmnAccess, SmnReg};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32, pub ThreadEnable [THREADS OFFSET(0) NUMBITS(32) []]];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Ccd,
    Ccx,
    Core,
    Thread,
    Unavailable,
    AlreadyEnabled,
    BspMissing,
    ThreadLayout,
    Capacity,
    CacheState,
}

/// Release tuple: PHYSICAL CCD selects SMN, but LOGICAL complex/core
/// slots select the bit. Physical IDs are separately validated by Rs APOB.
#[derive(Debug, Clone, Copy)]
pub struct Thread {
    pub physical_ccd: u8,
    pub logical_ccx: u8,
    pub logical_core: u8,
    pub thread: u8,
    pub threads_per_core: u8,
}
impl Thread {
    pub fn register_and_mask(self) -> Result<(SmnReg<ThreadEnable::Register>, u32), Error> {
        if self.physical_ccd >= 12 {
            return Err(Error::Ccd);
        }
        if self.logical_ccx >= 2 {
            return Err(Error::Ccx);
        }
        if self.logical_core >= 8 {
            return Err(Error::Core);
        }
        if !(1..=2).contains(&self.threads_per_core) || self.thread >= self.threads_per_core {
            return Err(Error::Thread);
        }
        // The donor deliberately uses wrapping 32-bit arithmetic. CCD0..7
        // fold through F6000000h; CCD8..11 use the upper address region.
        let address = 0x3a08_1018u32
            .wrapping_add(u32::from(self.physical_ccd) * 0x0200_0000)
            .wrapping_add(if self.physical_ccd < 8 {
                0xf600_0000
            } else {
                0
            });
        let bit = self.logical_ccx * 16 + self.threads_per_core * self.logical_core + self.thread;
        Ok((SmnReg::new(address), 1 << bit))
    }
}

/// Derive every enabled AP release tuple, skipping only the logical-zero
/// BSP. SMT bit spacing follows the first logical core, as in ApobRsPei;
/// a thread1-only layout or missing logical BSP fails before SMN writes.
pub fn launch_order(
    map: &crate::apob::ccx::CoreMap<12>,
) -> Result<heapless::Vec<Thread, 383>, Error> {
    let ccd = map.ccds.first().ok_or(Error::BspMissing)?;
    let ccx = ccd.ccxs.first().ok_or(Error::BspMissing)?;
    let core = ccx.cores.first().ok_or(Error::BspMissing)?;
    if (ccd.logical, ccx.logical, core.logical) != (0, 0, 0) || !core.threads[0] {
        return Err(Error::BspMissing);
    }
    let threads_per_core = if core.threads[1] { 2 } else { 1 };
    let mut out = heapless::Vec::new();
    for ccd in &map.ccds {
        for ccx in &ccd.ccxs {
            for core in &ccx.cores {
                if (!core.threads[0] && core.threads[1])
                    || (threads_per_core == 1 && core.threads[1])
                {
                    return Err(Error::ThreadLayout);
                }
                for thread in 0..threads_per_core {
                    if !core.threads[usize::from(thread)]
                        || (ccd.logical, ccx.logical, core.logical, thread) == (0, 0, 0, 0)
                    {
                        continue;
                    }
                    let target = Thread {
                        physical_ccd: ccd.id,
                        logical_ccx: ccx.logical,
                        logical_core: core.logical,
                        thread,
                        threads_per_core,
                    };
                    target.register_and_mask()?;
                    out.push(target).map_err(|_| Error::Capacity)?;
                }
            }
        }
    }
    Ok(out)
}

/// Rs cache replay: only the ECAM MSR and architectural MTRRs/PAT come
/// from the BSP. TOP_MEM/TOM2 and SYS_CFG remain the AP's PSP-provided
/// state, with the documented routing-bit transformation below.
pub fn sipi_msrs(
    cpu: &impl fstart_arch::x86::cpu::msr::MsrAccess,
) -> Result<fstart_arch::x86::mtrr::SipiMsrs, Error> {
    use fstart_arch::x86::{
        cpu::amd::msr::MMIO_CFG_BASE,
        mtrr::{self, MsrEntry, SipiMsrs},
    };
    let snapshot =
        super::cache::fixed_dram_view(cpu, |cpu| mtrr::snapshot_with(|index| cpu.read(index)))
            .map_err(|_| Error::CacheState)?
            .map_err(|_| Error::CacheState)?;
    let mut out = SipiMsrs::new();
    out.push(MsrEntry::new(
        MMIO_CFG_BASE.index(),
        cpu.reg(MMIO_CFG_BASE).get(),
    ))
    .map_err(|_| Error::Capacity)?;
    for entry in snapshot {
        out.push(entry).map_err(|_| Error::Capacity)?;
    }
    Ok(out)
}

/// The AP modifies its OWN SYS_CFG after replay and before enabling caches,
/// matching the observed Rs AP entry, rather than copying BSP-only state.
pub fn sipi_unlock() -> fstart_arch::x86::mp::SipiUnlock {
    use fstart_arch::x86::{
        cpu::amd::msr::{SYSCFG, Syscfg},
        mp::SipiUnlock,
    };
    SipiUnlock::new(SYSCFG, Syscfg::MTRR_FIX_DRAM_MOD_EN::SET).with_post_replay(
        Syscfg::MTRR_FIX_DRAM_EN::SET + Syscfg::MTRR_VAR_DRAM_EN::SET,
        Syscfg::TOM2_EN::SET,
        Syscfg::TOM2_WB::SET,
    )
}

/// # Safety
/// The tuple came from validated live enabled topology, is not the BSP,
/// and a compatible AP reset vector/context/stack is already prepared.
/// The caller owns the SMN pair and subsequent AP synchronization.
pub unsafe fn enable_thread(smn: &impl SmnAccess, thread: Thread) -> Result<(), Error> {
    let (register, mask) = thread.register_and_mask()?;
    let register = smn.reg(register);
    let before = register.read(ThreadEnable::THREADS);
    if before == u32::MAX {
        return Err(Error::Unavailable);
    }
    if before & mask != 0 {
        return Err(Error::AlreadyEnabled);
    }
    register.write(ThreadEnable::THREADS.val(before | mask));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;
    #[test]
    fn launch_bits_use_logical_slots_not_physical_complex_or_core_ids() {
        use crate::apob::ccx::{Ccd, Ccx, Core, CoreMap};
        let mut map = CoreMap::<12> {
            ccds: [Ccd {
                logical: 0,
                id: 8,
                ccxs: [
                    Ccx {
                        logical: 0,
                        id: 1,
                        cores: [
                            Core {
                                logical: 0,
                                id: 5,
                                threads: [true, true],
                            },
                            Core {
                                logical: 1,
                                id: 2,
                                threads: [true, true],
                            },
                        ]
                        .into_iter()
                        .collect(),
                    },
                    Ccx {
                        logical: 1,
                        id: 0,
                        cores: [Core {
                            logical: 0,
                            id: 3,
                            threads: [true, true],
                        }]
                        .into_iter()
                        .collect(),
                    },
                ]
                .into_iter()
                .collect(),
            }]
            .into_iter()
            .collect(),
        };
        let order = launch_order(&map).unwrap();
        assert_eq!(order.len(), 5);
        let targets: std::vec::Vec<_> = order
            .iter()
            .map(|t| {
                let (r, m) = t.register_and_mask().unwrap();
                (r.address(), m)
            })
            .collect();
        assert_eq!(
            targets,
            [
                (0x4a08_1018, 2),
                (0x4a08_1018, 4),
                (0x4a08_1018, 8),
                (0x4a08_1018, 1 << 16),
                (0x4a08_1018, 1 << 17)
            ]
        );
        map.ccds[0].ccxs[0].cores[0].threads = [false, true];
        assert!(matches!(launch_order(&map), Err(Error::BspMissing)));
        map.ccds[0].ccxs[0].cores[0].threads = [true, false];
        assert!(matches!(launch_order(&map), Err(Error::ThreadLayout)));
        assert!(matches!(
            launch_order(&CoreMap::<12>::default()),
            Err(Error::BspMissing)
        ));
    }

    #[test]
    fn replay_does_not_transplant_bsp_syscfg_or_top_mem() {
        use core::cell::RefCell;
        use fstart_arch::x86::cpu::msr::testing::MsrModel;
        struct Cpu {
            syscfg: u64,
            writes: std::vec::Vec<(u32, u64)>,
        }
        impl MsrModel for Cpu {
            fn read(&mut self, index: u32) -> u64 {
                match index {
                    0xc001_0010 => self.syscfg,
                    0xc001_0058 => 0xe000_0021,
                    0xfe => 0x108,
                    0x2ff => 0xc00,
                    0x277 => 0x0007_0406_0007_0406,
                    0x250 | 0x258 | 0x259 | 0x268..=0x26f => {
                        assert_ne!(self.syscfg & (1 << 19), 0);
                        0x1e1e_1e1e_1e1e_1e1e
                    }
                    0x200..=0x20f => 0,
                    _ => panic!("unexpected BSP register {index:x}"),
                }
            }
            fn write(&mut self, index: u32, value: u64) {
                assert_eq!(index, 0xc001_0010);
                self.syscfg = value;
                self.writes.push((index, value));
            }
        }
        let cpu = RefCell::new(Cpu {
            syscfg: 0x740000,
            writes: std::vec::Vec::new(),
        });
        let entries = sipi_msrs(&cpu).unwrap();
        assert_eq!(cpu.borrow().syscfg, 0x740000);
        assert_eq!(entries[0].index, 0xc001_0058);
        assert_eq!(entries[0].value(), 0xe000_0021);
        assert_eq!(entries.last().unwrap().index, 0x2ff);
        assert!(
            !entries
                .iter()
                .any(|e| [0xc001_0010, 0xc001_001a, 0xc001_001d].contains(&e.index))
        );
    }

    fn thread(ccd: u8) -> Thread {
        Thread {
            physical_ccd: ccd,
            logical_ccx: 0,
            logical_core: 0,
            thread: 0,
            threads_per_core: 2,
        }
    }

    #[test]
    fn bank_transition_and_high_thread_bit() {
        for (ccd, address) in [
            (0, 0x3008_1018),
            (7, 0x3e08_1018),
            (8, 0x4a08_1018),
            (11, 0x5008_1018),
        ] {
            assert_eq!(
                thread(ccd).register_and_mask().unwrap().0.address(),
                address
            );
        }
        let high = Thread {
            logical_ccx: 1,
            logical_core: 7,
            thread: 1,
            ..thread(0)
        };
        assert_eq!(high.register_and_mask().unwrap().1, 1 << 31);
    }
    #[test]
    fn fails_before_writes_and_preserves_other_threads() {
        let hw = SmnSpace::default();
        let smn = hw.smn();
        hw.set(0x3008_1018u32, 0x80);
        unsafe { enable_thread(&smn, thread(0)) }.unwrap();
        assert_eq!(hw.get(0x3008_1018u32), 0x81);
        assert_eq!(
            unsafe { enable_thread(&smn, thread(0)) },
            Err(Error::AlreadyEnabled)
        );
        assert_eq!(thread(12).register_and_mask().err(), Some(Error::Ccd));
        assert_eq!(
            Thread {
                thread: 1,
                threads_per_core: 1,
                ..thread(0)
            }
            .register_and_mask()
            .err(),
            Some(Error::Thread)
        );
        hw.set(0x3008_1018u32, u32::MAX);
        assert_eq!(
            unsafe { enable_thread(&smn, thread(0)) },
            Err(Error::Unavailable)
        );
    }
}
