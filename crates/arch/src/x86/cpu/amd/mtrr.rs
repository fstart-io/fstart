//! The AMD64 fixed-MTRR extension: each fixed range's type byte has
//! RdDram (bit 4) and WrDram (bit 3), honoured with SYSCFG.MtrrFixDramEn
//! and readable/writable only with SYSCFG.MtrrFixDramModEn (AMD APM vol. 2
//! §7.9.1).

use super::msr::{SYSCFG, Syscfg};
use crate::x86::cpu::msr::MsrAccess;
use crate::x86::mtrr::{FIXED_MSRS, MTRR_TYPE_WRITE_BACK};
use tock_registers::interfaces::{Readable, Writeable};

/// RdDram | WrDram of a fixed-range type byte.
const RD_WR_DRAM: u8 = 0x18;

/// Run `f` with SYSCFG.MtrrFixDramModEn set, then write SYSCFG back as it
/// was. Exclusive access to this CPU's MTRRs (no APs, interrupts or SMIs
/// meanwhile) is the caller's.
pub fn with_fixed_dram_mod<M: MsrAccess + ?Sized, T>(cpu: &M, f: impl FnOnce(&M) -> T) -> T {
    let syscfg = cpu.reg(SYSCFG).get();
    cpu.reg(SYSCFG)
        .set(syscfg | Syscfg::MTRR_FIX_DRAM_MOD_EN::SET.value);
    let result = f(cpu);
    cpu.reg(SYSCFG).set(syscfg);
    result
}

/// A fixed MTRR with every write-back range routed to DRAM and every other
/// range to MMIO.
pub const fn wb_ranges_to_dram(value: u64) -> u64 {
    let mut bytes = value.to_le_bytes();
    let mut i = 0;
    while i < bytes.len() {
        bytes[i] = if bytes[i] & 7 == MTRR_TYPE_WRITE_BACK as u8 {
            bytes[i] | RD_WR_DRAM
        } else {
            bytes[i] & !RD_WR_DRAM
        };
        i += 1;
    }
    u64::from_le_bytes(bytes)
}

/// Route every write-back fixed range to DRAM and the others to MMIO, as
/// AGESA does. With SYSCFG.MtrrFixDramEn set, an access in a fixed range
/// goes to DRAM only if its RdDram/WrDram bits say so; the generic MTRR
/// code writes the types only and leaves the bits the firmware before it
/// left. Caches are written back before and after; only changed MTRRs are
/// written.
///
/// # Safety
/// Ring 0 with caching enabled (CR0.CD clear); as for
/// [`with_fixed_dram_mod`].
pub unsafe fn route_fixed_wb_to_dram<M: MsrAccess + ?Sized>(cpu: &M) {
    // SAFETY: the caller's contract.
    unsafe { crate::x86::writeback_invalidate_caches() };
    with_fixed_dram_mod(cpu, |cpu| {
        for index in FIXED_MSRS {
            let old = cpu.read(index);
            let new = wb_ranges_to_dram(old);
            if new != old {
                cpu.write(index, new);
            }
            fstart_log::info!("mtrr: fixed {:#x} {:#x} -> {:#x}", index, old, new);
        }
    });
    // SAFETY: as above.
    unsafe { crate::x86::writeback_invalidate_caches() };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wb_ranges_go_to_dram() {
        // WB, WB|RdDram, UC|WrDram|RdDram, WP|WrDram.
        assert_eq!(wb_ranges_to_dram(0x0d18_1606), 0x0500_1e1e);
    }
}
