// SPDX-License-Identifier: MIT OR Apache-2.0
//! MP init of a noncar SoC with the architectural MP engine: the SMU
//! releases the APs ([`fstart_driver_amd::smu::threads::launch`]), every
//! CPU loads its microcode and runs the SoC's CCX init
//! ([`super::cpu::Fam17hCpu`]). The APs replay the BSP's MTRRs and PAT
//! before enabling caches.
use super::{Soc, cpu::Fam17hCpu};
use core::sync::atomic::{AtomicU32, Ordering};
use fstart_arch::x86::{
    cpu::{
        amd::msr::{SYSCFG, Syscfg},
        msr::{MsrAccess, NativeMsr},
    },
    mp::{self, CpuDriver, MpConfig, MpError, MpHandle, SipiUnlock},
    mtrr::{self, SipiMsrs},
};
use fstart_driver_amd::{
    ccx::{Thread, cache, tsc::Tsc},
    df::Die,
    smu::threads::{self, Policy},
};
use fstart_pci::ecam::GlobalEcam;
use heapless::Vec;
use tock_registers::interfaces::ReadWriteable;

/// The CPUs MP init brought up.
pub struct Cpus {
    pub mp: MpHandle,
    /// APIC IDs, BSP first.
    pub apic_ids: Vec<u32, 32>,
}

/// The patch for these CPUs from `microcode` and the CCX init `init` on
/// every CPU, the SMU releases the APs of `dies`.
pub fn start<S: Soc, E>(
    dies: &[Die<S, GlobalEcam>],
    microcode: Option<&'static [u8]>,
    init: impl Fn(&NativeMsr, Thread) -> Result<(), E> + Send + Sync,
    clock: Tsc,
) -> Result<Cpus, &'static str> {
    let threads = threads::thread_count(dies, Policy::DEFAULT).map_err(|_| "CPU topology")?;
    let cpu = Fam17hCpu::<S, _>::new(S::CPU_NAME, microcode, init);
    let drivers: [&dyn CpuDriver; 1] = [&cpu];
    let mut launch = |aps: u16| {
        // SAFETY: the reset vector sends APs to the SIPI trampoline the MP
        // engine installed before calling this.
        unsafe { threads::launch(dies, Policy::DEFAULT, usize::from(aps), &mut { clock }) }.map_err(
            |error| {
                if let threads::Error::ApCount { started, requested } = error {
                    fstart_log::error!("{}: started {} of {} APs", S::NAME, started, requested);
                }
                1
            },
        )
    };
    let mp = mp::mp_init_with_launcher(
        &MpConfig {
            cpu_drivers: &drivers,
            num_cpus: threads as u16,
            max_cpus: S::MAX_CPUS,
        },
        &mut launch,
    );
    let mp = match mp {
        Ok(mp) => {
            fstart_log::info!(
                "{}: {} CPUs up, {} CPU init errors",
                S::NAME,
                mp.num_cpus(),
                cpu.errors()
            );
            mp
        }
        Err(error) => {
            match error {
                MpError::NoApsResponded => fstart_log::error!("mp: no AP responded"),
                MpError::PartialBringup { expected, actual } => {
                    fstart_log::error!("mp: {} of {} APs responded", actual, expected)
                }
                MpError::PlatformLaunchFailed(_) => {
                    fstart_log::error!("mp: SMU release failed")
                }
                MpError::HardwareCpuCountExceedsCapacity { reported, .. } => {
                    fstart_log::error!("mp: {} CPUs responded", reported)
                }
                _ => fstart_log::error!("mp: setup failed"),
            }
            return Err("MP init");
        }
    };
    if cpu.errors() != 0 {
        return Err("CPU initialization");
    }
    let apic_ids: [AtomicU32; 32] = [const { AtomicU32::new(u32::MAX) }; 32];
    mp.scope(|scope| {
        scope.scatter(&|cpu: u32| {
            if let Some(slot) = apic_ids.get(cpu as usize) {
                slot.store(fstart_arch::x86::cpuid(1).1 >> 24, Ordering::Release);
            }
        })
    });
    let apic_ids = apic_ids
        .iter()
        .map(|id| id.load(Ordering::Acquire))
        .take(usize::from(mp.num_cpus()))
        .collect();
    Ok(Cpus { mp, apic_ids })
}

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
