// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso-specific bootstrap policy for the reusable architectural MP engine.
//! Cache preparation and one-shot SMU launch adaptation, not completed MP integration.
mod release;
use fstart_arch::x86::mp::cache::{State, Write};
use fstart_driver_amd::picasso::{
    cache,
    cpu::{self, Msr},
};
pub use release::ColdRelease;

#[derive(Debug)]
pub enum Error {
    Cpu(cpu::Error),
    AmdCache(cache::Error),
    Image(fstart_arch::x86::mp::cache::Error),
}

/// BSP only after native early init has installed the validated AMD cache plan;
/// before any AP release, with interrupts/SMI excluded and exclusive MSR access.
/// Preserve the actual AMD fixed attributes, TOM/TOM2, SYSCFG and PAT. The AP
/// assembly requires a BSP-inclusive, all-AP cache-off rendezvous and serializes
/// the image writes. Target cache/update/SMI semantics remain integration gates.
/// Returned state still needs resident publication by the MP engine; it is not
/// proof of the platform's physical mapping/coherency or SMM policy.
pub fn cache_state(msr: &mut impl Msr) -> Result<State, Error> {
    cpu::memory_limits(msr).map_err(Error::Cpu)?;
    let syscfg = msr.read(cpu::SYSCFG);
    let prefix = [
        Write::new(cpu::SYSCFG, syscfg | (1 << 19)),
        Write::new(cpu::TOP_MEM, msr.read(cpu::TOP_MEM)),
        Write::new(cpu::TOP_MEM2, msr.read(cpu::TOP_MEM2)),
    ];
    let suffix = [Write::new(cpu::SYSCFG, syscfg)];
    cache::fixed_dram_view(msr, |msr| {
        State::capture(|index| msr.read(index), &prefix, &suffix)
    })
    .map_err(Error::AmdCache)?
    .map_err(Error::Image)
    .map(State::require_global_cache_off)
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
    fn captures_amd_routing_and_restores_original_bsp_gate() {
        let original = (1 << 18) | (1 << 20) | (1 << 21) | (1 << 22);
        let mut cpu = Cpu {
            syscfg: original,
            writes: std::vec::Vec::new(),
            cap: 0x108,
        };
        let state = cache_state(&mut cpu).unwrap();
        assert!(state.global_cache_off());
        assert_eq!(cpu.syscfg, original);
        assert_eq!(
            cpu.writes.as_slice(),
            &[(cpu::SYSCFG, original | (1 << 19)), (cpu::SYSCFG, original)]
        );
        let writes: std::vec::Vec<_> = state
            .writes()
            .iter()
            .map(|w| (w.index(), w.value()))
            .collect();
        assert_eq!(
            &writes[..4],
            &[
                (0x2ff, 0),
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
        // Even a bounded-image rejection closes the BSP's temporary gate.
        cpu.cap = 0x1ff;
        assert!(matches!(cache_state(&mut cpu), Err(Error::Image(_))));
        assert_eq!(cpu.syscfg, original);
    }
    #[test]
    fn encrypted_or_missing_fixed_state_has_no_bsp_writes() {
        for (syscfg, cap) in [((1 << 23) | (1 << 21), 0x108), (1 << 21, 8)] {
            let mut cpu = Cpu {
                syscfg,
                cap,
                writes: std::vec::Vec::new(),
            };
            assert!(matches!(cache_state(&mut cpu), Err(Error::AmdCache(_))));
            assert!(cpu.writes.is_empty());
        }
    }
}
