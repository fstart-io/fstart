// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h policy for the architectural MP engine: the MSR image APs
//! replay before enabling caches, and (Picasso) AP release through the SMU.
use fstart_arch::x86::mtrr::{self, MsrEntry, SipiMsrs};
use fstart_driver_amd::fam17h::{
    cache,
    msr::{self as cpu, Msr},
};
#[cfg(feature = "picasso")]
use fstart_driver_amd::picasso::{
    registers::Smn,
    topology::{self, Topology},
};

#[derive(Debug)]
pub enum Error {
    Cpu(cpu::MemoryBoundary),
    AmdCache(cache::Error),
    Capacity,
}

/// BSP only, after the AMD cache plan is installed and before AP release.
///
/// The fixed MTRRs are read with their
/// RdDram/WrDram routing exposed (SYSCFG.MtrrFixDramModEn). APs replay
/// SYSCFG with that gate open, TOP_MEM/TOM2, the MTRRs and PAT, then the
/// BSP's SYSCFG and finally MTRR_DEF_TYPE.
pub fn sipi_msrs(msr: &mut impl Msr) -> Result<SipiMsrs, Error> {
    cpu::memory_limits(msr).map_err(Error::Cpu)?;
    let syscfg = msr.read(cpu::SYSCFG);
    let mut msrs = SipiMsrs::new();
    for entry in [
        MsrEntry::new(cpu::SYSCFG, syscfg | cpu::SYSCFG_MTRR_FIX_DRAM_MOD_EN),
        MsrEntry::new(cpu::TOP_MEM, msr.read(cpu::TOP_MEM)),
        MsrEntry::new(cpu::TOP_MEM2, msr.read(cpu::TOP_MEM2)),
    ] {
        msrs.push(entry).map_err(|_| Error::Capacity)?;
    }
    let mtrrs = cache::fixed_dram_view(msr, |msr| mtrr::snapshot_with(|index| msr.read(index)))
        .map_err(Error::AmdCache)?
        .map_err(|_| Error::Capacity)?;
    let (def_type, mtrrs) = mtrrs.split_last().ok_or(Error::Capacity)?;
    for entry in mtrrs
        .iter()
        .copied()
        .chain([MsrEntry::new(cpu::SYSCFG, syscfg), *def_type])
    {
        msrs.push(entry).map_err(|_| Error::Capacity)?;
    }
    Ok(msrs)
}

#[cfg(feature = "picasso")]
/// Release `ap_count` APs through SMU ThreadEn. Released APs start at the
/// reset vector, where `picasso.S` sends them to the SIPI trampoline.
/// # Safety
/// Cold boot, all APs still held, `topology` is the applied topology, and the
/// SIPI trampoline is installed. Exclusive SMN access.
pub unsafe fn release_aps(
    smn: &mut impl Smn,
    topology: Topology,
    ap_count: u16,
    delay_us: &mut impl FnMut(u32),
) -> Result<(), topology::Error> {
    unsafe { topology::launch(smn, topology, usize::from(ap_count), delay_us) }.map(|_| ())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    struct Cpu {
        syscfg: u64,
        writes: std::vec::Vec<(u32, u64)>,
        cap: u64,
    }
    impl Msr for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            match index {
                cpu::SYSCFG => self.syscfg,
                cpu::TOP_MEM => 0x80000000,
                cpu::TOP_MEM2 => 0x140000000,
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
            assert_eq!(index, cpu::SYSCFG);
            self.syscfg = value;
            self.writes.push((index, value));
        }
    }
    #[test]
    fn replays_amd_routing_around_mtrrs_and_restores_bsp_gate() {
        let original = (1 << 18) | (1 << 20) | (1 << 21) | (1 << 22);
        let mut cpu = Cpu {
            syscfg: original,
            writes: std::vec::Vec::new(),
            cap: 0x108,
        };
        let msrs = sipi_msrs(&mut cpu).unwrap();
        assert_eq!(cpu.syscfg, original);
        let writes: std::vec::Vec<_> = msrs.iter().map(|w| (w.index, w.value())).collect();
        assert_eq!(
            &writes[..3],
            &[
                (cpu::SYSCFG, original | (1 << 19)),
                (cpu::TOP_MEM, 0x80000000),
                (cpu::TOP_MEM2, 0x140000000)
            ]
        );
        assert!(writes.contains(&(0x250, 0x1e1e1e1e1e1e1e1e)));
        assert!(writes.contains(&(0x259, 0)));
        assert_eq!(
            &writes[writes.len() - 2..],
            &[(cpu::SYSCFG, original), (0x2ff, 0xc00)]
        );
    }
    #[test]
    fn encrypted_or_missing_fixed_state_has_no_bsp_writes() {
        for (syscfg, cap) in [((1 << 23) | (1 << 21), 0x108), (1 << 21, 8)] {
            let mut cpu = Cpu {
                syscfg,
                cap,
                writes: std::vec::Vec::new(),
            };
            assert!(matches!(sipi_msrs(&mut cpu), Err(Error::AmdCache(_))));
            assert!(cpu.writes.is_empty());
        }
    }
}
